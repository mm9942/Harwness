//! Geteilte Typen für `doc.read_pdf` — der Vertrag zwischen `native.rs` und
//! `mistral.rs`.
//!
//! Spezifikationsquelle: `doc_read_pdf_design.md`, Abschnitt „Geteilte Typen
//! (Vertrag)".
//!
//! # Verantwortung
//! Dieses Modul besitzt die Typen, auf die sich [`crate::native`] und
//! [`crate::mistral`] einigen, ohne dass eines der beiden Module vom anderen
//! abhängt: den geparsten Seitenbereich ([`PageRange`]), die
//! Backend-Kennung ([`Backend`]) und das extrahierte Ergebnis
//! ([`ExtractedPage`], [`ExtractedDocument`]). Weder Datei-I/O noch
//! Netz-Aufrufe — reine Daten und Parsing.
//!
//! # Schlüsseltypen
//! - [`PageRange`] — 1-basierter, inklusiver Seitenbereich.
//! - [`Backend`] — welches Backend ein [`ExtractedDocument`] geliefert hat.
//! - [`ExtractedPage`] — Text einer einzelnen extrahierten Seite.
//! - [`ExtractedDocument`] — das Gesamtergebnis einer Extraktion.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, keine Sperren,
//! keine Threads.
//!
//! # Fehler
//! [`PageRange::parse`] liefert
//! [`crate::error::DocToolError::InvalidPageRange`] bei ungültiger Eingabe.
//!
//! # Examples
//! ```rust
//! use harw_tool_doc::types::PageRange;
//!
//! let range = PageRange::parse("2-5").unwrap();
//! assert!(range.contains(3));
//! assert!(!range.contains(6));
//! ```

use crate::error::{DocToolError, DocToolResult};

/// 1-basierter, inklusiver Seitenbereich. `last == None` bedeutet: bis zum
/// Dokumentende.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRange {
    /// Erste eingeschlossene Seite (1-basiert, mindestens 1).
    pub first: u32,
    /// Letzte eingeschlossene Seite, oder `None` für „bis Dokumentende".
    pub last: Option<u32>,
}

impl PageRange {
    /// Parst einen vom Modell übergebenen Seitenbereich.
    ///
    /// # Description
    /// Unterstützt drei Formen: eine einzelne Seite (`"3"` → Seite 3 bis 3),
    /// einen geschlossenen Bereich (`"2-5"`) und einen ab einer Seite
    /// offenen Bereich (`"4-"` → Seite 4 bis Dokumentende). Umgebender
    /// Leerraum wird getrimmt.
    ///
    /// # Arguments
    /// - `raw` (`&str`): die vom Modell übergebene Zeichenkette, z. B. `"2-5"`.
    ///
    /// # Returns
    /// Der geparste [`PageRange`].
    ///
    /// # Errors
    /// - [`DocToolError::InvalidPageRange`]: leere Eingabe, keine gültige
    ///   Zahl, eine Seitenzahl `0`, oder `first` liegt hinter `last`.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_doc::types::PageRange;
    ///
    /// assert_eq!(PageRange::parse("3").unwrap(), PageRange { first: 3, last: Some(3) });
    /// assert_eq!(PageRange::parse("4-").unwrap(), PageRange { first: 4, last: None });
    /// assert!(PageRange::parse("0").is_err());
    /// assert!(PageRange::parse("5-2").is_err());
    /// ```
    pub fn parse(raw: &str) -> DocToolResult<Self> {
        let invalid = |reason: &str| DocToolError::InvalidPageRange {
            raw: raw.to_owned(),
            reason: reason.to_owned(),
        };

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(invalid("leerer Seitenbereich"));
        }

        let (first_part, last_part) = match trimmed.split_once('-') {
            Some((first, rest)) => (first, Some(rest)),
            None => (trimmed, None),
        };

        let first: u32 = first_part
            .trim()
            .parse()
            .map_err(|_| invalid("erste Seite ist keine gültige Zahl"))?;
        if first == 0 {
            return Err(invalid("Seitenzahlen sind 1-basiert, '0' ist ungültig"));
        }

        let last = match last_part {
            None => Some(first),
            Some(rest) if rest.trim().is_empty() => None,
            Some(rest) => {
                let parsed: u32 = rest
                    .trim()
                    .parse()
                    .map_err(|_| invalid("letzte Seite ist keine gültige Zahl"))?;
                if parsed == 0 {
                    return Err(invalid("Seitenzahlen sind 1-basiert, '0' ist ungültig"));
                }
                Some(parsed)
            }
        };

        if let Some(last_page) = last
            && last_page < first
        {
            return Err(invalid("erste Seite liegt hinter der letzten"));
        }

        Ok(Self { first, last })
    }

    /// Prüft, ob die 1-basierte Seite `number` im Bereich liegt.
    ///
    /// # Arguments
    /// - `number` (`u32`): die zu prüfende, 1-basierte Seitenzahl.
    ///
    /// # Returns
    /// `true`, wenn `number >= first` und (falls `last` gesetzt ist)
    /// `number <= last`.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_doc::types::PageRange;
    ///
    /// let open = PageRange { first: 4, last: None };
    /// assert!(open.contains(100));
    /// assert!(!open.contains(3));
    /// ```
    #[must_use]
    pub fn contains(&self, number: u32) -> bool {
        number >= self.first && self.last.is_none_or(|last| number <= last)
    }
}

/// Kennzeichnet, welches Backend ein [`ExtractedDocument`] geliefert hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Mistral OCR ([`crate::mistral`]).
    MistralOcr,
    /// Lokale Textextraktion über `oxidize-pdf` ([`crate::native`]).
    Native,
}

impl Backend {
    /// Kurzes, für Menschen und das Modell lesbares Etikett.
    ///
    /// # Description
    /// Nicht Teil des ursprünglichen Typ-Vertrags, sondern eine additive
    /// Hilfsmethode für den Kopf der `doc.read_pdf`-Ausgabe
    /// (`{path} — Seiten X–Y von N ({label})`, siehe
    /// [`crate::tool::DocReadPdfExecutor`]).
    ///
    /// # Returns
    /// `"Mistral OCR"` bzw. `"lokal"`.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_doc::types::Backend;
    ///
    /// assert_eq!(Backend::Native.label(), "lokal");
    /// assert_eq!(Backend::MistralOcr.label(), "Mistral OCR");
    /// ```
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MistralOcr => "Mistral OCR",
            Self::Native => "lokal",
        }
    }
}

/// Extrahierter Text einer einzelnen Seite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPage {
    /// 1-basierte Seitenzahl innerhalb des Dokuments.
    pub number: u32,
    /// Extrahierter Text bzw. Markdown der Seite.
    pub text: String,
}

/// Ergebnis einer Extraktion: die angeforderten Seiten plus Metadaten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedDocument {
    /// Gesamtseitenzahl des Dokuments, falls das Backend sie kennt.
    pub total_pages: Option<u32>,
    /// Die angeforderten Seiten, aufsteigend nach [`ExtractedPage::number`]
    /// sortiert. Leer, wenn der angeforderte Bereich außerhalb des
    /// Dokuments liegt.
    pub pages: Vec<ExtractedPage>,
    /// Welches Backend dieses Ergebnis geliefert hat.
    pub backend: Backend,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_page_range_parse_single_page() -> TestResult {
        let range = PageRange::parse("3").map_err(ctx("parse('3')"))?;
        assert_eq!(
            range,
            PageRange {
                first: 3,
                last: Some(3)
            }
        );
        Ok(())
    }

    #[test]
    fn test_page_range_parse_closed_range() -> TestResult {
        let range = PageRange::parse("2-5").map_err(ctx("parse('2-5')"))?;
        assert_eq!(
            range,
            PageRange {
                first: 2,
                last: Some(5)
            }
        );
        Ok(())
    }

    #[test]
    fn test_page_range_parse_open_range() -> TestResult {
        let range = PageRange::parse("4-").map_err(ctx("parse('4-')"))?;
        assert_eq!(
            range,
            PageRange {
                first: 4,
                last: None
            }
        );
        Ok(())
    }

    #[test]
    fn test_page_range_parse_trims_whitespace() -> TestResult {
        let range = PageRange::parse("  2 - 5  ").map_err(ctx("parse('  2 - 5  ')"))?;
        assert_eq!(
            range,
            PageRange {
                first: 2,
                last: Some(5)
            }
        );
        Ok(())
    }

    #[test]
    fn test_page_range_parse_rejects_zero() -> TestResult {
        let Err(_) = PageRange::parse("0") else {
            return Err(TestError::Unexpected(
                "'0' muss abgelehnt werden".to_owned(),
            ));
        };
        let Err(_) = PageRange::parse("0-5") else {
            return Err(TestError::Unexpected(
                "'0-5' muss abgelehnt werden".to_owned(),
            ));
        };
        Ok(())
    }

    #[test]
    fn test_page_range_parse_rejects_first_after_last() -> TestResult {
        let Err(err) = PageRange::parse("5-2") else {
            return Err(TestError::Unexpected(
                "'5-2' muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(matches!(err, DocToolError::InvalidPageRange { .. }));
        Ok(())
    }

    #[test]
    fn test_page_range_parse_rejects_garbage() -> TestResult {
        for raw in ["abc", "", "   ", "2-x"] {
            let Err(_) = PageRange::parse(raw) else {
                return Err(TestError::Unexpected(format!(
                    "{raw:?} muss abgelehnt werden"
                )));
            };
        }
        Ok(())
    }

    #[test]
    fn test_page_range_contains_closed() -> TestResult {
        let range = PageRange::parse("2-5").map_err(ctx("parse('2-5')"))?;
        assert!(!range.contains(1));
        assert!(range.contains(2));
        assert!(range.contains(5));
        assert!(!range.contains(6));
        Ok(())
    }

    #[test]
    fn test_page_range_contains_open() -> TestResult {
        let range = PageRange::parse("4-").map_err(ctx("parse('4-')"))?;
        assert!(!range.contains(3));
        assert!(range.contains(4));
        assert!(range.contains(1000));
        Ok(())
    }

    #[test]
    fn test_backend_label() {
        assert_eq!(Backend::MistralOcr.label(), "Mistral OCR");
        assert_eq!(Backend::Native.label(), "lokal");
    }
}
