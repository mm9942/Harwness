//! Crate-weiter Fehlertyp für `harw-tool-doc`.
//!
//! Spezifikationsquelle: `doc_read_pdf_design.md`, Abschnitt „Fehler".
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich [`DocToolError`] — die vollständige
//! Fehlermenge von `doc.read_pdf`: Argumentfehler
//! ([`DocToolError::InvalidPageRange`]), Datei-Vorprüfung
//! ([`DocToolError::NotAPdf`], [`DocToolError::TooLarge`]), lokale
//! Extraktion ([`DocToolError::Parse`], [`DocToolError::Encrypted`],
//! [`DocToolError::ExtractionPanicked`]), Mistral OCR
//! ([`DocToolError::MistralApi`], [`DocToolError::MistralResponse`],
//! [`DocToolError::InvalidBaseUrl`]) und Infrastruktur (`Io`, `Egress`,
//! `Http`, `Timeout`, `Join`, `AlreadyConfigured`). Netz- und
//! Extraktions-Entscheidungen selbst treffen [`crate::mistral`] und
//! [`crate::native`]; dieses Modul trägt nur die Fehlerform.
//!
//! # Schlüsseltypen
//! - [`DocToolError`] — Fehler-Enum, per `#[derive(harw_macros::HarwError)]`
//!   um `Display`, `std::error::Error` (inklusive `source()`) und die
//!   `From`-Impls der `#[from]`-Varianten ergänzt.
//! - `DocToolResult<T>` — vom Derive erzeugter Ergebnis-Alias.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, keine Sperren,
//! keine Threads.
//!
//! # Examples
//! ```rust
//! use harw_tool_doc::error::DocToolError;
//!
//! let err = DocToolError::NotAPdf { path: "report.txt".to_owned() };
//! assert!(err.to_string().contains("report.txt"));
//! ```

use harw_macros::HarwError;

/// Fehler aller Werkzeuge in `harw-tool-doc`, insbesondere `doc.read_pdf`.
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zur Diagnose nötig
/// ist, ohne dass Quellcode gelesen werden muss. Die Meldungen sind für die
/// Weitergabe an das Modell gedacht (`ToolOutput::error(err.to_string())`,
/// siehe [`crate::tool::DocReadPdfExecutor`]); keine Variante enthält den
/// vollen Antwort-Körper eines Mistral-Aufrufs oder Zugangsdaten.
///
/// # Concurrency
/// `Send + Sync`; kein geteilter veränderlicher Zustand.
#[derive(Debug, HarwError)]
pub enum DocToolError {
    /// Das `pages`-Argument von `doc.read_pdf` ist kein gültiger
    /// Seitenbereich (siehe [`crate::types::PageRange::parse`]).
    #[msg("ungültiger Seitenbereich '{raw}': {reason}")]
    InvalidPageRange {
        /// Der ungeparste, vom Modell übergebene Wert.
        raw: String,
        /// Menschenlesbarer Grund der Ablehnung.
        reason: String,
    },

    /// Die geöffnete Datei beginnt nicht mit der PDF-Signatur `%PDF-`.
    #[msg("'{path}' ist keine PDF-Datei (Signatur '%PDF-' fehlt)")]
    NotAPdf {
        /// Der vom Modell übergebene, workspace-relative Pfad.
        path: String,
    },

    /// Die Datei überschreitet [`crate::tool::MAX_PDF_BYTES`].
    #[msg("'{path}' ist mit {bytes} Bytes größer als das Limit von {limit} Bytes")]
    TooLarge {
        /// Der vom Modell übergebene, workspace-relative Pfad.
        path: String,
        /// Tatsächliche (bzw. beim Kappen mindestens gemessene) Dateigröße in Bytes.
        bytes: u64,
        /// Durchgesetztes Limit in Bytes.
        limit: u64,
    },

    /// Datei- oder Pfadfehler beim symlinkfreien Öffnen unterhalb der
    /// Workspace-Wurzel (`harw_fsutil::open_beneath`), oder ein sonstiger
    /// I/O-Fehler.
    #[from]
    #[msg("Datei-Fehler: {0}")]
    Io(std::io::Error),

    /// Die Egress-Richtlinie für den Mistral-OCR-Client ließ sich nicht
    /// bauen (siehe `MistralOcrClient::new` in [`crate::mistral`]).
    #[from]
    #[msg("Egress-Fehler: {0}")]
    Egress(harw_egress::EgressError),

    /// Transportfehler des Mistral-OCR-HTTP-Clients (DNS, TLS,
    /// Verbindungsabbruch, Timeout einer Einzelanfrage).
    #[from]
    #[msg("HTTP-Transportfehler: {0}")]
    Http(reqwest::Error),

    /// Mistral OCR antwortete mit einem Status außerhalb von 2xx.
    #[msg("Mistral OCR antwortete mit Status {status}: {message}")]
    MistralApi {
        /// HTTP-Statuscode der Antwort.
        status: u16,
        /// Fehlermeldung aus `{"object":"error","message":…}`, sonst der
        /// (auf 500 Zeichen gekürzte) Rohtext der Antwort.
        message: String,
    },

    /// Die Mistral-OCR-Antwort war zwar 2xx, aber unerwartet geformt oder
    /// unlesbar (fehlendes Feld, ungültiges JSON).
    #[msg("Mistral OCR lieferte eine unerwartete Antwort: {reason}")]
    MistralResponse {
        /// Menschenlesbarer Grund.
        reason: String,
    },

    /// Die konfigurierte `base_url` des Mistral-Providers lässt sich nicht
    /// als Basis-URL verwenden (kein Host, kein `http`/`https`-Schema, …).
    #[msg("ungültige Mistral-Basis-URL '{base_url}': {reason}")]
    InvalidBaseUrl {
        /// Die konfigurierte Basis-URL.
        base_url: String,
        /// Menschenlesbarer Grund.
        reason: String,
    },

    /// Lokale Extraktion: `oxidize-pdf` konnte die Datei nicht parsen.
    #[msg("PDF konnte nicht gelesen werden: {reason}")]
    Parse {
        /// `Display`-Text des `oxidize-pdf`-`ParseError`.
        reason: String,
    },

    /// Lokale Extraktion: die PDF ist verschlüsselt, und ein leeres
    /// Passwort hat nicht geholfen.
    #[msg("PDF ist verschlüsselt; ein leeres Passwort hat nicht geholfen")]
    Encrypted,

    /// Lokale Extraktion: `oxidize-pdf` ist beim Öffnen, Zählen der Seiten
    /// oder Extrahieren einer Seite in Panik geraten (über `catch_unwind`
    /// aufgefangen).
    #[msg("Extraktion (Seite {page:?}) ist in der PDF-Bibliothek abgestürzt")]
    ExtractionPanicked {
        /// Die betroffene 1-basierte Seite, oder `None` beim Öffnen/Zählen
        /// der Seiten.
        page: Option<u32>,
    },

    /// Die native Extraktion hat ihr Zeitbudget überschritten
    /// (`tokio::time::timeout` um `native::extract_pages`).
    #[msg("Zeitüberschreitung nach {seconds}s")]
    Timeout {
        /// Das durchgesetzte Zeitbudget in Sekunden.
        seconds: u64,
    },

    /// Eine `spawn_blocking`-Hintergrundaufgabe ist abgebrochen oder in
    /// Panik geraten (`tokio::task::JoinError`, als Text gekapselt statt als
    /// `#[from]`, weil `JoinError` weder `Clone` noch `PartialEq`
    /// beisteuert, die dieses Enum sonst überall bräuchte).
    #[msg("Hintergrundaufgabe abgebrochen: {reason}")]
    Join {
        /// `Display`-Text des `JoinError`.
        reason: String,
    },

    /// [`crate::mistral::install_mistral_ocr`] wurde bereits einmal
    /// erfolgreich aufgerufen; ein zweiter Aufruf ändert die Konfiguration
    /// nicht.
    #[msg("Mistral OCR für doc.read_pdf wurde bereits konfiguriert")]
    AlreadyConfigured,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Die Meldung nennt den betroffenen Pfad, ohne dass Quellcode nötig ist.
    #[test]
    fn test_not_a_pdf_message_contains_path() {
        let err = DocToolError::NotAPdf {
            path: "report.txt".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("report.txt"), "unerwartet: {message}");
    }

    /// Größe und Limit stehen beide in der Meldung.
    #[test]
    fn test_too_large_message_contains_bytes_and_limit() {
        let err = DocToolError::TooLarge {
            path: "big.pdf".to_owned(),
            bytes: 999,
            limit: 100,
        };
        let message = err.to_string();
        assert!(message.contains("999"), "unerwartet: {message}");
        assert!(message.contains("100"), "unerwartet: {message}");
    }

    /// Rohwert und Ablehnungsgrund stehen beide in der Meldung.
    #[test]
    fn test_invalid_page_range_message_contains_raw_and_reason() {
        let err = DocToolError::InvalidPageRange {
            raw: "5-2".to_owned(),
            reason: "erste Seite liegt hinter der letzten".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("5-2"), "unerwartet: {message}");
        assert!(
            message.contains("hinter der letzten"),
            "unerwartet: {message}"
        );
    }

    /// Status und Antworttext stehen beide in der Meldung.
    #[test]
    fn test_mistral_api_message_contains_status_and_body() {
        let err = DocToolError::MistralApi {
            status: 429,
            message: "rate limited".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("429"), "unerwartet: {message}");
        assert!(message.contains("rate limited"), "unerwartet: {message}");
    }

    /// Die betroffene Seite steht in der Meldung; `None` bleibt lesbar.
    #[test]
    fn test_extraction_panicked_message_names_page() {
        let err = DocToolError::ExtractionPanicked { page: Some(3) };
        assert!(err.to_string().contains("Some(3)"));

        let err = DocToolError::ExtractionPanicked { page: None };
        assert!(err.to_string().contains("None"));
    }

    /// `#[from]` erzeugt die Konvertierung, die `?` für `std::io` braucht, und
    /// `source()` verweist auf den gekapselten Fehler.
    #[test]
    fn test_from_io_error_maps_to_io_variant_with_source() {
        use std::error::Error as _;

        let err = DocToolError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "fehlt"));
        assert!(matches!(err, DocToolError::Io(_)));
        assert!(err.source().is_some(), "source() muss die Ursache liefern");
    }

    /// `#[from]` erzeugt die Konvertierung für `harw_egress::EgressError`.
    #[test]
    fn test_from_egress_error_maps_to_egress_variant() -> TestResult {
        let Err(egress_err) = harw_egress::EgressPolicy::new(vec!["*.docs.rs".to_owned()], false)
        else {
            return Err(TestError::Unexpected(
                "ein Wildcard-Eintrag muss EgressPolicy::new scheitern lassen".to_owned(),
            ));
        };
        let err = DocToolError::from(egress_err);
        assert!(matches!(err, DocToolError::Egress(_)));
        Ok(())
    }

    /// Der vom Derive erzeugte Alias existiert und ist verwendbar.
    #[test]
    fn test_doc_tool_result_alias_is_usable() {
        fn ok() -> DocToolResult<u8> {
            Ok(7)
        }
        assert_eq!(ok().ok(), Some(7));
    }
}
